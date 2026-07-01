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
    EmbeddedSpan {
        prefix: String,
        suffix: String,
        data: Value,
    },
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
/// over the token floor so it is scanned but (correctly) left untouched by the
/// lossless pass.
fn prose_thread() -> String {
    let para = "The quarterly review covered onboarding, retention, and the support \
        backlog in depth. Several reviewers noted that response times improved after \
        the new routing policy, though the weekend coverage gap remains a concern. \
        Action items were assigned across the team with owners and target dates. ";
    para.repeat(12)
}

// ── Other content types: code, logs, chat ───────────────────────────────────
// These are the workloads that dominate real traffic. Honest accounting:
//   * Code  → `compact_code` is whitespace-only and OPT-IN (`code_compaction`).
//   * Logs  → `compact_log` (template-collapse) lives in the LOSSY pass (gated
//             by per-tenant `allow_lossy`); near-lossless for true repeats.
//   * Chat  → `extract_prose` (salience line-drop) is LOSSY (allow_lossy).
// The lossy/dedup passes are async + Redis-backed in production, so here we call
// their pure algorithm functions directly to measure what they achieve.

/// Sloppy code: trailing whitespace on every line and runs of blank lines — the
/// case `compact_code` was built for.
fn code_messy() -> String {
    let mut s = String::from("```rust\n");
    for i in 0..45 {
        s.push_str(&format!(
            "    let value_{i} = compute(input_{i}, {i});      \n"
        ));
        if i % 3 == 0 {
            s.push_str("\n\n\n");
        }
    }
    s.push_str("```");
    s
}

/// Already-clean code: `compact_code` should find almost nothing (honest 0%-ish).
fn code_clean() -> String {
    let mut s = String::from("```rust\n");
    for i in 0..45 {
        s.push_str(&format!("    let value_{i} = compute(input_{i}, {i});\n"));
    }
    s.push_str("```");
    s
}

/// A repetitive application log: many lines sharing a template (collapsible) plus
/// a couple of error lines that must survive verbatim.
fn repetitive_log() -> String {
    let mut s = String::new();
    for i in 0..80 {
        s.push_str(&format!(
            "2026-06-30T10:{:02}:{:02} INFO request handled route=/v1/chat status=200 dur={}ms\n",
            i / 60,
            i % 60,
            12 + (i % 7)
        ));
    }
    s.push_str("2026-06-30T10:01:30 ERROR upstream timeout after 30000ms route=/v1/chat\n");
    s.push_str("2026-06-30T10:01:31 WARN retry budget exhausted for upstream gpu-7\n");
    s
}

/// A verbose chat/agent turn: substantive lines mixed with low-value filler and
/// near-duplicate restatements — what `extract_prose` is meant to thin out.
fn redundant_chat() -> String {
    let mut s = String::new();
    s.push_str("Thanks for the detailed write-up, this is really helpful context.\n");
    s.push_str("The production incident started around 02:00 UTC when latency spiked.\n");
    for i in 0..20 {
        s.push_str(&format!(
            "Just to confirm, we are still on track for the milestone, right? ({i})\n"
        ));
    }
    s.push_str("Root cause was a connection-pool exhaustion in the payments service.\n");
    s.push_str("The fix raises the pool ceiling and adds a circuit breaker on timeouts.\n");
    for i in 0..20 {
        s.push_str(&format!(
            "Sounds good, looking forward to it, let me know if you need anything. ({i})\n"
        ));
    }
    s.push_str("Please prioritize the circuit-breaker change before the weekend freeze.\n");
    s
}

// ── Cross-turn dedup (lossless): repeated context across turns ────────────────

/// A reference document of the kind that gets re-grounded verbatim across turns
/// (RAG chunks, a pasted spec, a large tool result). ~1.2k tokens, over the floor.
fn big_doc() -> String {
    let mut s = String::new();
    for i in 0..40 {
        s.push_str(&format!(
            "Section {i}: endpoint /v{i} accepts a JSON payload and returns a normalized \
             result object carrying status, latency_ms, and a request identifier.\n"
        ));
    }
    s
}

/// A multi-turn conversation where the same reference doc is sent THREE times as
/// full message segments — exactly the shape cross-turn dedup targets.
fn conversation_with_repeated_context() -> Value {
    let doc = big_doc();
    json!({
        "model": "verify",
        "messages": [
            {"role": "system", "content": doc},
            {"role": "user", "content": "Based on the reference above, what does endpoint /v3 do?"},
            {"role": "assistant", "content": "It accepts a JSON payload and returns a normalized result."},
            {"role": "user", "content": doc},
            {"role": "assistant", "content": "Understood — I have the reference in context."},
            {"role": "user", "content": doc},
            {"role": "user", "content": "Now summarize what every endpoint returns."}
        ]
    })
}

/// Sum the heuristic token count across every message content segment of a body.
fn count_body_tokens(json: &Value) -> u32 {
    let tk = HeuristicTokenizer::new();
    let mut total = 0u32;
    if let Some(msgs) = json.get("messages").and_then(|m| m.as_array()) {
        for m in msgs {
            match m.get("content") {
                Some(Value::String(s)) => total += tk.count_text(s),
                Some(Value::Array(parts)) => {
                    for p in parts {
                        if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                            total += tk.count_text(t);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    total
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
            Verify::EmbeddedSpan {
                prefix,
                suffix,
                data,
            } => {
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

    Metrics {
        before,
        after,
        compressed: stats.compressed,
        changed,
        lossless,
    }
}

fn prod_cfg() -> CompressionBoonSettings {
    // Exactly the production defaults except the master switch is on.
    CompressionBoonSettings {
        enabled: true,
        ..Default::default()
    }
}

fn aggressive_cfg() -> CompressionBoonSettings {
    CompressionBoonSettings {
        enabled: true,
        min_tokens: 16,
        ..Default::default()
    }
}

// ── Regression assertions ────────────────────────────────────────────────────

/// Every corpus fixture behaves as expected at the PRODUCTION default settings,
/// and every compressed segment is provably lossless.
#[test]
fn corpus_behaves_as_expected_at_production_defaults() {
    let cfg = prod_cfg();
    for fix in corpus() {
        let m = run(&fix, &cfg);
        assert!(
            m.lossless,
            "{}: NOT lossless (changed={})",
            fix.name, m.changed
        );
        match fix.expect {
            Expect::Compresses => assert!(
                m.compressed >= 1 && m.after < m.before,
                "{}: expected compression, got before={} after={} compressed={}",
                fix.name,
                m.before,
                m.after,
                m.compressed
            ),
            Expect::LeavesUnchanged => assert!(
                m.compressed == 0 && !m.changed,
                "{}: expected unchanged, but it was modified (compressed={})",
                fix.name,
                m.compressed
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
    assert_eq!(
        at_default.compressed, 0,
        "small array should be below the 512 floor"
    );
    assert!(!at_default.changed);

    // … and at a lower floor it compresses, still losslessly.
    let at_low = run(&fix, &aggressive_cfg());
    assert!(
        at_low.compressed >= 1 && at_low.after < at_low.before,
        "lowering the floor should compress it"
    );
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
            if m.lossless {
                "OK ✓"
            } else {
                "FAIL ✗"
            }
        } else if m.lossless {
            "— (kept)"
        } else {
            "FAIL ✗"
        };
        println!(
            "{:<30} {:>10} {:>10} {:>8.1}% {:>6} {:>10}  {}",
            fix.name,
            m.before,
            m.after,
            m.saved_pct(),
            m.compressed,
            lossless,
            fix.shape
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

// ── Other-content-type measurement (code / logs / chat) ──────────────────────

/// Compact a fenced code segment through the real `apply` path with code
/// compaction enabled. Returns the rewritten text.
fn run_code(content: &str) -> String {
    let cfg = CompressionBoonSettings {
        enabled: true,
        min_tokens: 16,
        code_compaction: true,
        ..Default::default()
    };
    let mut body = json!({"model": "verify", "messages": [{"role": "user", "content": content}]});
    let _ = compression::apply(&cfg, true, &mut body);
    body["messages"][0]["content"].as_str().unwrap().to_string()
}

fn pct(before: u32, after: u32) -> f64 {
    if before == 0 {
        0.0
    } else {
        (before - after) as f64 / before as f64 * 100.0
    }
}

fn print_other_content_report() {
    let tk = HeuristicTokenizer::new();
    let q: std::collections::HashSet<String> = [
        "incident", "root", "cause", "circuit", "breaker", "payments",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    // (label, before_text, after_text, pass, default-state, note)
    let code_messy_in = code_messy();
    let code_clean_in = code_clean();
    let log_in = repetitive_log();
    let chat_in = redundant_chat();

    let rows: Vec<(&str, String, String, &str, &str)> = vec![
        (
            "code_messy (trailing ws/blanks)",
            code_messy_in.clone(),
            run_code(&code_messy_in),
            "lossless",
            "opt-in: code_compaction",
        ),
        (
            "code_clean (well-formatted)",
            code_clean_in.clone(),
            run_code(&code_clean_in),
            "lossless",
            "opt-in: code_compaction",
        ),
        (
            "repetitive_log (templated)",
            log_in.clone(),
            compression::compact_log(&log_in).unwrap_or_else(|| log_in.clone()),
            "lossy*",
            "opt-in: compact_logs",
        ),
        (
            "redundant_chat (verbose turn)",
            chat_in.clone(),
            compression::extract_prose(&chat_in, &q, 0.4).unwrap_or_else(|| chat_in.clone()),
            "lossy",
            "opt-in: allow_lossy",
        ),
    ];

    println!("\nOTHER CONTENT TYPES — code / logs / chat (what dominates real traffic)");
    println!("{}", "─".repeat(104));
    println!(
        "{:<34} {:>10} {:>10} {:>9} {:>10}  {}",
        "fixture", "tok_before", "tok_after", "saved", "pass", "default"
    );
    println!("{}", "─".repeat(104));
    for (label, before_t, after_t, pass, default_state) in &rows {
        let before = tk.count_text(before_t);
        let after = tk.count_text(after_t);
        println!(
            "{:<34} {:>10} {:>10} {:>8.1}% {:>10}  {}",
            label,
            before,
            after,
            pct(before, after),
            pass,
            default_state
        );
    }
    println!("{}", "─".repeat(104));
    println!("* log template-collapse is reversible and near-lossless for genuine repeats (errors/warns kept verbatim).");
    println!("Code is whitespace-only (no AST/semantic compaction). Chat line-dropping is genuinely lossy.");
    println!("None of these are on by default today — that is the real gap for your workload, not arrays.");
}

fn print_dedup_report() {
    let cfg = CompressionBoonSettings {
        enabled: true,
        min_tokens: 128,
        ..Default::default()
    };
    let mut body = conversation_with_repeated_context();
    let msg_count = body["messages"].as_array().unwrap().len();
    let before = count_body_tokens(&body);
    let (stats, stash) = compression::dedup_rewrite(&cfg, &mut body);
    let after = count_body_tokens(&body);

    println!("\nCROSS-TURN DEDUP — lossless removal of context re-sent across turns");
    println!("{}", "─".repeat(104));
    println!("conversation: a reference doc re-sent 3× across {msg_count} messages");
    println!("  duplicates found : {}", stats.duplicates);
    println!("  refs created     : {} (later copies → [ref:HASH]; original stashed for retrieve_original)", stats.refs_created);
    println!("  tokens before    : {before}");
    println!(
        "  tokens after     : {after}  ({:.1}% saved)",
        pct(before, after)
    );
    println!(
        "  reversible       : {} originals stashed (fully recoverable)",
        stash.len()
    );
    println!(
        "default: opt-in (per-tenant `dedup` policy). LOSSLESS — the first copy is kept verbatim."
    );
}

/// Prints the full verification report. Run with:
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
    print_other_content_report();
    print_dedup_report();
    println!(
        "\nNote: 'plain_prose_thread_control' and 'redundant_chat' show the deterministic \
         ceiling on human prose — only the lossy line-dropper touches chat, and only when \
         allow_lossy is on. Real, faithful chat/prose compression is Phase 2 (neural Compressor).\n"
    );
}

// ── Regression assertions for code / logs / chat ─────────────────────────────

#[test]
fn code_compaction_strips_whitespace_when_enabled() {
    let tk = HeuristicTokenizer::new();
    let messy = code_messy();
    let out = run_code(&messy);
    assert!(
        tk.count_text(&out) < tk.count_text(&messy),
        "messy code should shrink with code_compaction on"
    );
    // The fence is preserved and no trailing-space lines remain.
    assert!(out.starts_with("```rust\n"));
    assert!(
        !out.lines().any(|l| l.ends_with(' ')),
        "trailing whitespace should be gone"
    );
}

#[test]
fn code_compaction_is_off_by_default() {
    // Same messy code, but through the production path (code_compaction off) is untouched.
    let messy = code_messy();
    let mut body =
        json!({"model": "verify", "messages": [{"role": "user", "content": messy.clone()}]});
    let stats = compression::apply(&prod_cfg(), false, &mut body);
    assert_eq!(
        stats.compressed, 0,
        "code must not be touched unless code_compaction is enabled"
    );
    assert_eq!(body["messages"][0]["content"].as_str().unwrap(), messy);
}

#[test]
fn log_template_collapse_shrinks_and_keeps_errors() {
    let tk = HeuristicTokenizer::new();
    let log = repetitive_log();
    let out = compression::compact_log(&log).expect("repetitive log should collapse");
    assert!(tk.count_text(&out) < tk.count_text(&log));
    // Error/warn lines survive verbatim.
    assert!(out.contains("ERROR upstream timeout after 30000ms"));
    assert!(out.contains("WARN retry budget exhausted"));
}

#[test]
fn chat_prose_extraction_thins_filler() {
    let tk = HeuristicTokenizer::new();
    let chat = redundant_chat();
    let q: std::collections::HashSet<String> = [
        "incident", "root", "cause", "circuit", "breaker", "payments",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let out = compression::extract_prose(&chat, &q, 0.4).expect("verbose chat should thin");
    assert!(tk.count_text(&out) < tk.count_text(&chat));
    // The substantive, query-relevant line is retained.
    assert!(out.contains("connection-pool exhaustion in the payments service"));
}

#[test]
fn cross_turn_dedup_removes_repeated_blocks_losslessly() {
    let cfg = CompressionBoonSettings {
        enabled: true,
        min_tokens: 128,
        ..Default::default()
    };
    let mut body = conversation_with_repeated_context();
    let before = count_body_tokens(&body);
    let (stats, stash) = compression::dedup_rewrite(&cfg, &mut body);
    let after = count_body_tokens(&body);

    // The doc appears 3× → first occurrence kept, the two later copies deduped.
    assert_eq!(
        stats.refs_created, 2,
        "two later duplicates should be replaced"
    );
    assert!(
        after < before,
        "dedup should reduce total tokens (got {before}→{after})"
    );

    let msgs = body["messages"].as_array().unwrap();
    // First copy (system message) is verbatim; later copies are pointers.
    assert!(
        msgs[0]["content"].as_str().unwrap().contains("Section 0:"),
        "first copy kept verbatim"
    );
    assert!(
        msgs[3]["content"].as_str().unwrap().starts_with("[ref:"),
        "2nd copy → ref marker"
    );
    assert!(
        msgs[5]["content"].as_str().unwrap().starts_with("[ref:"),
        "3rd copy → ref marker"
    );

    // Reversibility: the stash holds the exact original behind every ref.
    assert_eq!(stash.len(), 2);
    assert!(stash
        .iter()
        .all(|(_, original)| original.contains("Section 0:")));
}

// ── Property/fuzz: the structural codec is lossless on ARBITRARY JSON ─────────
// Curated fixtures prove known shapes; this proves the invariant across thousands
// of randomly-generated values, including CSV-hostile strings (commas, quotes,
// newlines, unicode, the literal "null"), sparse object arrays, and nesting.

/// Tiny deterministic xorshift64 RNG — no external crate.
struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
}

/// A scalar, sometimes carrying characters that stress CSV escaping / typing.
fn gen_scalar(rng: &mut Rng) -> Value {
    match rng.below(6) {
        0 => Value::Null,
        1 => Value::Bool(rng.next_u64() & 1 == 0),
        2 => json!(rng.next_u64() as i64 % 100_000 - 50_000),
        3 => {
            // Deliberately nasty strings, incl. ones that collide with typed forms.
            let choices = [
                "plain",
                "a,b",
                "has\"quote",
                "line\nbreak",
                "café ☕",
                "",
                "null",
                "true",
                "123",
            ];
            json!(choices[rng.below(choices.len() as u64) as usize])
        }
        4 => json!((rng.next_u64() % 1000) as f64 / 7.0),
        _ => json!(format!("s{}", rng.below(1000))),
    }
}

/// Object with 1..=5 keys from a small pool, so arrays of these objects sometimes
/// share a key set (uniform) and sometimes do not (sparse / union-of-keys).
fn gen_object(rng: &mut Rng, depth: u32) -> Value {
    let keys = [
        "id", "name", "email", "role", "active", "score", "meta", "tags",
    ];
    let count = 1 + rng.below(5) as usize;
    let mut map = serde_json::Map::new();
    for _ in 0..count {
        let k = keys[rng.below(keys.len() as u64) as usize];
        map.insert(k.to_string(), gen_value(rng, depth.saturating_sub(1)));
    }
    Value::Object(map)
}

fn gen_value(rng: &mut Rng, depth: u32) -> Value {
    if depth == 0 {
        return gen_scalar(rng);
    }
    match rng.below(6) {
        0 | 1 => gen_scalar(rng),
        2 => {
            // Array of objects — the qualifying-table candidate.
            let n = 2 + rng.below(6) as usize;
            Value::Array((0..n).map(|_| gen_object(rng, depth)).collect())
        }
        3 => {
            let n = rng.below(5) as usize;
            Value::Array((0..n).map(|_| gen_value(rng, depth - 1)).collect())
        }
        4 => gen_object(rng, depth),
        _ => {
            // Object wrapping an array-of-objects → exercises compact_recursive.
            let n = 2 + rng.below(6) as usize;
            let arr: Vec<Value> = (0..n).map(|_| gen_object(rng, depth)).collect();
            json!({"wrapped": arr, "note": gen_scalar(rng)})
        }
    }
}

/// A sizable array of near-schema objects: a fixed column set per array with keys
/// occasionally dropped (sparsity) and CSV-hostile / nested random values. This
/// mirrors real tabular data, so the table encoder reliably wins — while still
/// exercising sparse rows, nasty strings, nulls, and nested cells.
fn gen_rows(rng: &mut Rng, n: usize) -> Vec<Value> {
    let pool = [
        "id", "name", "email", "role", "active", "score", "meta", "tags",
    ];
    let ncols = 4 + rng.below(4) as usize; // 4..=7 columns
    let schema: Vec<&str> = pool.iter().take(ncols).copied().collect();
    (0..n)
        .map(|_| {
            let mut m = serde_json::Map::new();
            for k in &schema {
                // Dense (light ~8% sparsity), mostly scalar cells — the shape that
                // actually compresses. A rare nested cell keeps that path covered.
                if rng.below(12) == 0 {
                    continue;
                }
                let cell = if rng.below(8) == 0 {
                    gen_value(rng, 1)
                } else {
                    gen_scalar(rng)
                };
                m.insert((*k).to_string(), cell);
            }
            if m.is_empty() {
                m.insert("id".to_string(), json!(rng.below(1000) as i64));
            }
            Value::Object(m)
        })
        .collect()
}

/// A compaction-friendly root: a sizable object-array, that array wrapped in an
/// object, or two such arrays in one object (exercises the recursive path too).
fn gen_root(rng: &mut Rng) -> Value {
    let n = 8 + rng.below(24) as usize;
    let arr = gen_rows(rng, n);
    match rng.below(3) {
        0 => Value::Array(arr),
        1 => json!({"data": arr, "meta": {"total": n, "page": rng.below(9) as i64}}),
        _ => {
            let n2 = 8 + rng.below(16) as usize;
            let arr2 = gen_rows(rng, n2);
            json!({"first": arr, "second": arr2, "note": gen_scalar(rng)})
        }
    }
}

#[test]
fn fuzz_structural_codec_is_always_lossless() {
    let mut compacted = 0u32;
    for seed in 1..=3000u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let v = gen_root(&mut rng);
        let text = serde_json::to_string(&v).unwrap();

        // The codec operates on PARSED JSON: a request arrives as text, is parsed
        // once, and that parsed Value is the source of truth the gateway forwards.
        // So the losslessness oracle is `from_str(text)`, not the pre-serialization
        // value `v` — serde_json's default float parser is not correctly-rounded,
        // so `v` and `from_str(to_string(v))` can differ by 1 ULP on some floats.
        // The gateway never sees `v`; it sees `from_str(text)`.
        let expected: Value = serde_json::from_str(&text).unwrap();

        if let Some(out) = structural_json::compact(&text) {
            compacted += 1;
            // Decode the structural form; fall back to a plain-JSON parse for the
            // minify path (which decode_for_verification returns None for).
            let decoded = structural_json::decode_for_verification(&out)
                .or_else(|| serde_json::from_str::<Value>(&out).ok());
            assert_eq!(
                decoded.as_ref(),
                Some(&expected),
                "seed {seed}: structural compaction was NOT lossless\n input:  {text}\n output: {out}"
            );
            assert!(
                out.len() < text.len(),
                "seed {seed}: compaction must be strictly smaller"
            );
        }
    }
    // Make sure the corpus actually exercised the compaction path a lot.
    assert!(
        compacted > 200,
        "fuzz corpus barely triggered compaction ({compacted}) — generator too weak"
    );
}

#[test]
fn log_rewrite_collapses_and_is_reversible() {
    let cfg = CompressionBoonSettings {
        enabled: true,
        min_tokens: 16,
        ..Default::default()
    };
    let log = repetitive_log();
    let mut body = json!({"model": "verify", "messages": [{"role": "user", "content": log}]});
    let (stats, stash) = compression::log_rewrite(&cfg, &mut body);

    assert_eq!(
        stats.refs_created, 1,
        "the repetitive log segment should collapse"
    );
    let out = body["messages"][0]["content"].as_str().unwrap();
    assert!(
        out.contains("[ref:"),
        "collapsed segment carries a retrieval marker"
    );
    assert!(
        out.contains("ERROR upstream timeout"),
        "error lines must be kept verbatim"
    );
    // Reversible: the full original is stashed for retrieve_original.
    assert_eq!(stash.len(), 1);
    assert!(stash[0].1.contains("request handled"));
}

#[test]
fn log_rewrite_skips_structural_tables() {
    // A structurally-compacted table classifies as Log but must NOT be collapsed.
    let cfg = CompressionBoonSettings {
        enabled: true,
        min_tokens: 16,
        ..Default::default()
    };
    let mut table = String::from("OBLETH_TABLE rows=60\nid,name,active\n");
    for i in 0..60 {
        table.push_str(&format!("{i},user{i},true\n"));
    }
    let snapshot = table.clone();
    let mut body = json!({"model": "verify", "messages": [{"role": "user", "content": table}]});
    let (stats, _) = compression::log_rewrite(&cfg, &mut body);
    assert_eq!(
        stats.refs_created, 0,
        "structural table must be left alone by the log pass"
    );
    assert_eq!(body["messages"][0]["content"].as_str().unwrap(), snapshot);
}

#[test]
fn neural_select_kept_drops_low_scored_sentences() {
    let sentences: Vec<String> = (0..10)
        .map(|i| format!("This is sentence number {i} with some meaningful content here."))
        .collect();
    // Descending scores: sentence 0 has the highest score, sentence 9 the lowest.
    let scores: Vec<f32> = (0..10).map(|i| 1.0 - i as f32 * 0.1).collect();
    let query_terms = std::collections::HashSet::new();
    let out = super::compressor::select_kept(&sentences, &scores, 0.4, &query_terms)
        .expect("should produce a compressed result");
    // The lowest-scored sentences should be dropped.
    assert!(
        !out.contains("sentence number 9"),
        "lowest-scored sentence must be dropped"
    );
    assert!(
        !out.contains("sentence number 8"),
        "low-scored sentence must be dropped"
    );
    // Result must be strictly shorter than the joined original.
    let joined = sentences.join(" ");
    assert!(
        out.len() < joined.len(),
        "result must be shorter than original"
    );
    // Omission marker should be present.
    assert!(out.contains("sentences omitted]"));
}

#[test]
fn dedup_leaves_unique_content_untouched() {
    let cfg = CompressionBoonSettings {
        enabled: true,
        min_tokens: 16,
        ..Default::default()
    };
    let mut body = json!({
        "model": "verify",
        "messages": [
            {"role": "user", "content": "first unique message with enough length to clear the floor ".repeat(8)},
            {"role": "user", "content": "second DIFFERENT message with enough length to clear the floor ".repeat(8)}
        ]
    });
    let snapshot = body.clone();
    let (stats, stash) = compression::dedup_rewrite(&cfg, &mut body);
    assert_eq!(stats.refs_created, 0, "no duplicates → nothing replaced");
    assert!(stash.is_empty());
    assert_eq!(
        body, snapshot,
        "unique content must be byte-identical after dedup"
    );
}
