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

use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::{Context, Result};

use crate::admin::AdminClient;
use crate::report;

/// All knobs for a compression A/B run (mirrors ab.py's env surface).
pub struct CompressionConfig {
    pub proxy_base: String,
    pub admin_base: String,
    pub admin_token: String,
    pub api_key: String,
    pub model: String,
    pub price_in_per_mtok: f64,
    pub prefill_tps: Vec<u32>,
    pub max_tokens: u32,
    pub reps: u32,
    pub min_tokens: u32,
}

pub struct ArmResult {
    pub ms: f64,
    pub before: u64,
    pub after: u64,
    pub saved: u64,
}

pub struct SampleArms {
    pub off: ArmResult,
    pub default: ArmResult,
    pub lossy: ArmResult,
}

impl SampleArms {
    fn get(&self, arm: &str) -> &ArmResult {
        match arm {
            "off" => &self.off,
            "lossy" => &self.lossy,
            _ => &self.default,
        }
    }
}

/// (arm label, `x-obleth-boons` header value or None for the default arm).
const ARMS: [(&str, Option<&str>); 3] =
    [("off", Some("off")), ("default", None), ("lossy", Some("lossy"))];

/// POST one chat completion; return (elapsed_ms, x-obleth-compression header).
async fn chat(
    client: &reqwest::Client,
    cfg: &CompressionConfig,
    messages: &Messages,
    boons: Option<&str>,
) -> Result<(f64, Option<String>)> {
    let body = json!({
        "model": cfg.model, "messages": messages, "max_tokens": cfg.max_tokens, "stream": false,
    });
    let mut rb = client
        .post(format!("{}/v1/chat/completions", cfg.proxy_base))
        .bearer_auth(&cfg.api_key)
        .json(&body);
    if let Some(b) = boons {
        rb = rb.header("x-obleth-boons", b);
    }
    let t = Instant::now();
    let res = rb.send().await.context("POST /v1/chat/completions")?;
    let status = res.status();
    let comp = res
        .headers()
        .get("x-obleth-compression")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let _ = res.text().await;
    let elapsed = t.elapsed().as_secs_f64() * 1000.0;
    if !status.is_success() {
        anyhow::bail!("chat completion -> {status}");
    }
    Ok((elapsed, comp))
}

/// Median latency over `reps` timed calls (after one warm call).
async fn median_ms(
    client: &reqwest::Client,
    cfg: &CompressionConfig,
    messages: &Messages,
    boons: Option<&str>,
) -> Result<f64> {
    let _ = chat(client, cfg, messages, boons).await?; // warm
    let mut samples = Vec::with_capacity(cfg.reps as usize);
    for _ in 0..cfg.reps {
        samples.push(chat(client, cfg, messages, boons).await?.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Ok(samples[samples.len() / 2])
}

/// Run all three arms for one corpus sample.
async fn run_sample(
    client: &reqwest::Client,
    cfg: &CompressionConfig,
    messages: &Messages,
) -> Result<SampleArms> {
    let mut arms: BTreeMap<&str, ArmResult> = BTreeMap::new();
    for (name, hdr) in ARMS {
        let (_, comp) = chat(client, cfg, messages, hdr).await?;
        let (before, after, saved) = comp
            .as_deref()
            .and_then(parse_compression_header)
            .unwrap_or((0, 0, 0));
        let ms = median_ms(client, cfg, messages, hdr).await?;
        arms.insert(name, ArmResult { ms, before, after, saved });
    }
    Ok(SampleArms {
        off: arms.remove("off").unwrap(),
        default: arms.remove("default").unwrap(),
        lossy: arms.remove("lossy").unwrap(),
    })
}

/// Snapshot + enable compression and lower min_tokens for the run. Returns the
/// prior (enabled, min_tokens) so the caller can restore them.
async fn setup_boons(admin: &AdminClient, min_tokens: u32) -> Result<(bool, u32)> {
    let prev = admin.get_boons().await?;
    let prev_enabled = prev["compression_enabled"].as_bool().unwrap_or(true);
    let prev_min = prev["compression_min_tokens"].as_u64().unwrap_or(512) as u32;
    admin
        .set_boons(json!({ "compression_enabled": true, "compression_min_tokens": min_tokens }))
        .await?;
    println!(
        "[setup] compression_enabled=true, min_tokens {prev_min}->{min_tokens} \
         (allow_lossy unchanged; the lossy arm forces it per-request)"
    );
    Ok((prev_enabled, prev_min))
}

async fn restore_boons(admin: &AdminClient, prev: (bool, u32)) {
    let _ = admin
        .set_boons(json!({ "compression_enabled": prev.0, "compression_min_tokens": prev.1 }))
        .await;
    println!("[teardown] restored compression_min_tokens={}", prev.1);
}

/// Render the full markdown report from measured rows + size sweeps.
#[allow(clippy::type_complexity)]
pub fn render_report(
    cfg: &CompressionConfig,
    rows: &[(String, SampleArms)],
    sweeps: &[(String, &'static str, Vec<(usize, SampleArms)>)],
) -> String {
    let mut o = String::new();
    o.push_str("# Compression boon — back-to-back A/B\n\n");
    o.push_str(&format!(
        "Model `{}` via `{}` · {} reps (median) · max_tokens={} · input price ${}/1M tok.\n\n",
        cfg.model, cfg.proxy_base, cfg.reps, cfg.max_tokens, cfg.price_in_per_mtok
    ));
    o.push_str(
        "Three arms per sample: **off** (`x-obleth-boons: off`), **default** \
         (lossless + dedup + log passes), **lossy** (`x-obleth-boons: lossy`, adds the \
         prose pass). Token counts come from the `x-obleth-compression` response header.\n\n",
    );

    // Token savings (measured).
    o.push_str("## Token savings (measured)\n\n");
    o.push_str("| corpus | arm | tokens in | after | saved % | gateway +ms |\n");
    o.push_str("|---|---|--:|--:|--:|--:|\n");
    for (corpus, arms) in rows {
        let off_ms = arms.off.ms;
        for (name, _) in ARMS {
            let a = arms.get(name);
            let over = a.ms - off_ms;
            let saved_pct = pct(a.before, a.after);
            let (tin, aft) = if a.before > 0 {
                (a.before.to_string(), a.after.to_string())
            } else {
                ("-".to_string(), "-".to_string())
            };
            o.push_str(&format!(
                "| {} | {name} | {tin} | {aft} | {saved_pct:.1}% | {over:+.1} |\n",
                if name == "off" { corpus.as_str() } else { "" }
            ));
        }
    }
    o.push('\n');

    // Cost saved (measured/deterministic).
    o.push_str("## Cost saved per request (measured)\n\n");
    o.push_str(&format!(
        "Input tokens removed × ${}/1M. Deterministic — applies every request.\n\n",
        cfg.price_in_per_mtok
    ));
    o.push_str("| corpus | arm | tokens saved | $/req saved | $ / 1M req |\n");
    o.push_str("|---|---|--:|--:|--:|\n");
    for (corpus, arms) in rows {
        for name in ["default", "lossy"] {
            let saved = arms.get(name).saved;
            let per_req = saved as f64 * cfg.price_in_per_mtok / 1_000_000.0;
            o.push_str(&format!(
                "| {} | {name} | {saved} | ${per_req:.6} | ${:.2} |\n",
                if name == "default" { corpus.as_str() } else { "" },
                per_req * 1_000_000.0
            ));
        }
    }
    o.push('\n');

    // Net latency: measured overhead vs modeled upstream saving.
    o.push_str("## Net latency: measured overhead vs modeled upstream saving\n\n");
    o.push_str(
        "The fixture upstream does not scale latency with prompt size, so upstream saving is \
         **modeled**: `upstream_ms_saved = tokens_saved / prefill_tps`. Net = \
         `upstream_saved − gateway_overhead`. Positive = compression makes the request faster.\n\n",
    );
    for (label, arm, sweep) in sweeps {
        o.push_str(&format!(
            "Size sweep on **{label}** (measured on the `{arm}` arm):\n\n"
        ));
        o.push_str("| size | tokens saved | gateway +ms |");
        for t in &cfg.prefill_tps {
            o.push_str(&format!(" net @ {t} tok/s |"));
        }
        o.push('\n');
        o.push_str("|--:|--:|--:|");
        for _ in &cfg.prefill_tps {
            o.push_str("--:|");
        }
        o.push('\n');
        for (sz, arms) in sweep {
            let a = arms.get(arm);
            let over = a.ms - arms.off.ms;
            o.push_str(&format!("| {sz} | {} | {over:+.1} |", a.saved));
            for t in &cfg.prefill_tps {
                o.push_str(&format!(" {:+.0} |", net_ms(a.saved, *t, over)));
            }
            o.push('\n');
        }
        o.push('\n');
    }
    o.push_str(
        "> Reading it: where a `net` column turns positive, compression is a latency win at that \
         upstream prefill rate; below it, gateway overhead dominates and you pay for token/cost \
         savings only. The deterministic sweep is ~free at any size; the neural sweep only wins \
         once tokens saved / prefill-rate beats the sidecar overhead.\n",
    );
    o
}

/// Corpora, keyed by display label, matching ab.py.
fn corpora() -> Vec<(String, Messages)> {
    vec![
        ("logs (repetitive)".into(), logs_payload(120)),
        ("json (uniform array)".into(), json_payload(120)),
        ("code (whitespace)".into(), code_payload(40)),
        ("prose (human)".into(), prose_payload(6)),
        ("repeated (dedup)".into(), repeated_payload(120)),
    ]
}

/// Size sweeps: (label, arm that does the work, payload fn, sizes).
#[allow(clippy::type_complexity)]
fn sweep_specs() -> Vec<(String, &'static str, fn(usize) -> Messages, Vec<usize>)> {
    vec![
        ("logs (deterministic)".into(), "default", logs_payload as fn(usize) -> Messages, vec![20, 60, 120, 300, 600]),
        ("prose (neural lossy)".into(), "lossy", prose_payload as fn(usize) -> Messages, vec![2, 4, 8, 16, 32]),
    ]
}

/// Execute the A/B, write + print the report, restore settings on every path.
pub async fn run(cfg: &CompressionConfig) -> Result<i32> {
    if cfg.api_key.is_empty() || cfg.model.is_empty() {
        anyhow::bail!("compression benchmark needs --model and --api-key (a model granted the compression boon)");
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let admin = AdminClient::new(cfg.admin_base.clone(), cfg.admin_token.clone());

    // Enable + lower min_tokens only when an admin token is supplied.
    let prev = if cfg.admin_token.is_empty() {
        None
    } else {
        Some(setup_boons(&admin, cfg.min_tokens).await?)
    };

    // Run the A/B, but always restore — on success, error, or Ctrl-C. Rust has no
    // async Drop, so restore is explicit here (mirrors obench's load teardown).
    let work = async {
        let mut rows = Vec::new();
        for (label, msgs) in corpora() {
            rows.push((label, run_sample(&client, cfg, &msgs).await?));
        }
        let mut sweeps = Vec::new();
        for (label, arm, f, sizes) in sweep_specs() {
            let mut samples = Vec::new();
            for sz in sizes {
                samples.push((sz, run_sample(&client, cfg, &f(sz)).await?));
            }
            sweeps.push((label, arm, samples));
        }
        Ok::<_, anyhow::Error>((rows, sweeps))
    };

    let result = tokio::select! {
        r = work => r,
        _ = tokio::signal::ctrl_c() => Err(anyhow::anyhow!("interrupted")),
    };

    if let Some(prev) = prev {
        restore_boons(&admin, prev).await;
    }

    let (rows, sweeps) = result?;
    let report_md = render_report(cfg, &rows, &sweeps);
    let path = report::write_report("compression", &report_md)?;
    report::write_meta(
        "compression",
        &serde_json::json!({
            "model": cfg.model, "proxy": cfg.proxy_base, "reps": cfg.reps,
            "corpora": rows.iter().map(|(l, a)| serde_json::json!({
                "corpus": l,
                "default_saved_pct": pct(a.default.before, a.default.after),
                "lossy_saved_pct": pct(a.lossy.before, a.lossy.after),
            })).collect::<Vec<_>>(),
        }),
    )?;
    println!("\n{report_md}");
    println!("\n[written] {}", path.display());
    Ok(0)
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

    fn arm(ms: f64, before: u64, after: u64, saved: u64) -> ArmResult {
        ArmResult { ms, before, after, saved }
    }

    #[test]
    fn render_report_includes_measured_savings_and_labels_modeled() {
        let cfg = CompressionConfig {
            proxy_base: "http://localhost:8088".into(),
            admin_base: "http://localhost:9180".into(),
            admin_token: String::new(),
            api_key: "sk-x".into(),
            model: "demo-model".into(),
            price_in_per_mtok: 0.30,
            prefill_tps: vec![500, 2000],
            max_tokens: 1,
            reps: 5,
            min_tokens: 64,
        };
        let rows = vec![(
            "logs (repetitive)".to_string(),
            SampleArms {
                off: arm(10.0, 0, 0, 0),
                default: arm(11.0, 100, 40, 60),
                lossy: arm(12.0, 100, 40, 60),
            },
        )];
        let sweeps = vec![(
            "logs (deterministic)".to_string(),
            "default",
            vec![(20usize, SampleArms {
                off: arm(10.0, 0, 0, 0),
                default: arm(10.5, 200, 80, 120),
                lossy: arm(11.0, 200, 80, 120),
            })],
        )];
        let md = render_report(&cfg, &rows, &sweeps);
        assert!(md.contains("# Compression boon — back-to-back A/B"));
        assert!(md.contains("Token savings (measured)"));
        assert!(md.contains("modeled")); // the crossover section labels itself modeled
        assert!(md.contains("demo-model"));
        assert!(md.contains("60.0%") || md.contains("60%")); // logs default savings
    }
}
